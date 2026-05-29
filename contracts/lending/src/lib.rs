use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, symbol_short, Address,
    Env, Vec,
};

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum ContractError {
    /// Borrower already has an active loan that has not been repaid.
    LoanAlreadyActive = 1,
    /// No active loan found for the borrower.
    NoActiveLoan = 2,
    /// The voucher has already vouched for this borrower.
    DuplicateVouch = 3,
    /// Vouch stake must be greater than zero.
    ZeroStake = 4,
}

#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum LoanStatus {
    Active = 0,
    Repaid = 1,
    Defaulted = 2,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Loan {
    pub borrower: Address,
    pub amount: u64,
    pub status: LoanStatus,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Vouch {
    pub voucher: Address,
    pub stake: u64,
}

const TTL_THRESHOLD: u32 = 518_400;
const TTL_TARGET: u32 = 518_400;

fn loan_key(borrower: &Address) -> (soroban_sdk::Symbol, Address) {
    (symbol_short!("LOAN"), borrower.clone())
}

fn vouches_key(borrower: &Address) -> (soroban_sdk::Symbol, Address) {
    (symbol_short!("VOUCHES"), borrower.clone())
}

#[contract]
pub struct LendingContract;

#[contractimpl]
impl LendingContract {
    /// Request a new loan. Panics with [`ContractError::LoanAlreadyActive`] if
    /// the borrower already has an active (non-repaid, non-defaulted) loan.
    pub fn request_loan(env: Env, borrower: Address, amount: u64) {
        borrower.require_auth();

        let key = loan_key(&borrower);

        // #619: guard against overwriting an active loan
        if let Some(existing) = env.storage().persistent().get::<_, Loan>(&key) {
            if existing.status == LoanStatus::Active {
                panic_with_error!(&env, ContractError::LoanAlreadyActive);
            }
        }

        let loan = Loan {
            borrower: borrower.clone(),
            amount,
            status: LoanStatus::Active,
        };
        env.storage().persistent().set(&key, &loan);
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_THRESHOLD, TTL_TARGET);
    }

    /// Repay the active loan for the caller.
    ///
    /// # Errors
    /// - [`ContractError::NoActiveLoan`] if no active loan exists for the
    ///   borrower. Uses `panic_with_error!` for a structured contract error
    ///   instead of a raw `expect` panic string (#618).
    pub fn repay(env: Env, borrower: Address) {
        borrower.require_auth();

        let key = loan_key(&borrower);

        // #618: structured error — replaces expect("no active loan")
        let mut loan: Loan = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, ContractError::NoActiveLoan));

        if loan.status != LoanStatus::Active {
            panic_with_error!(&env, ContractError::NoActiveLoan);
        }

        loan.status = LoanStatus::Repaid;
        env.storage().persistent().set(&key, &loan);
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_THRESHOLD, TTL_TARGET);
    }

    /// Vouch for a borrower with a stake.
    ///
    /// # Errors
    /// - [`ContractError::ZeroStake`] if stake is 0 (#621)
    /// - [`ContractError::DuplicateVouch`] if the same voucher has already
    ///   vouched for this borrower, preventing unlimited vouch stacking (#620)
    pub fn vouch(env: Env, borrower: Address, voucher: Address, stake: u64) {
        voucher.require_auth();

        // #621: reject zero-stake vouches
        if stake == 0 {
            panic_with_error!(&env, ContractError::ZeroStake);
        }

        let key = vouches_key(&borrower);
        let mut vouches: Vec<Vouch> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(&env));

        // #620: each voucher address may only vouch once per borrower
        for v in vouches.iter() {
            if v.voucher == voucher {
                panic_with_error!(&env, ContractError::DuplicateVouch);
            }
        }

        vouches.push_back(Vouch {
            voucher: voucher.clone(),
            stake,
        });
        env.storage().persistent().set(&key, &vouches);
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_THRESHOLD, TTL_TARGET);
    }

    /// Returns the loan for a borrower, if any.
    pub fn get_loan(env: Env, borrower: Address) -> Option<Loan> {
        env.storage().persistent().get(&loan_key(&borrower))
    }

    /// Returns all vouches for a borrower.
    pub fn get_vouches(env: Env, borrower: Address) -> Vec<Vouch> {
        env.storage()
            .persistent()
            .get(&vouches_key(&borrower))
            .unwrap_or_else(|| Vec::new(&env))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::Env;

    fn setup() -> (Env, LendingContractClient<'static>) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register(LendingContract, ());
        let client = LendingContractClient::new(&env, &contract_id);
        (env, client)
    }

    // ── #619 tests ────────────────────────────────────────────────────────────

    #[test]
    fn test_request_loan_success() {
        let (env, client) = setup();
        let borrower = Address::generate(&env);
        client.request_loan(&borrower, &1000);
        let loan = client.get_loan(&borrower).unwrap();
        assert_eq!(loan.status, LoanStatus::Active);
        assert_eq!(loan.amount, 1000);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1)")]
    fn test_duplicate_loan_panics() {
        let (env, client) = setup();
        let borrower = Address::generate(&env);
        client.request_loan(&borrower, &1000);
        // Second request_loan while first is still active must panic
        client.request_loan(&borrower, &500);
    }

    #[test]
    fn test_request_loan_after_repay_succeeds() {
        let (env, client) = setup();
        let borrower = Address::generate(&env);
        client.request_loan(&borrower, &1000);
        client.repay(&borrower);
        // After repayment a new loan is allowed
        client.request_loan(&borrower, &500);
        let loan = client.get_loan(&borrower).unwrap();
        assert_eq!(loan.status, LoanStatus::Active);
        assert_eq!(loan.amount, 500);
    }

    // ── #618 tests ────────────────────────────────────────────────────────────

    #[test]
    fn test_repay_success() {
        let (env, client) = setup();
        let borrower = Address::generate(&env);
        client.request_loan(&borrower, &1000);
        client.repay(&borrower);
        let loan = client.get_loan(&borrower).unwrap();
        assert_eq!(loan.status, LoanStatus::Repaid);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #2)")]
    fn test_repay_no_loan_panics() {
        let (env, client) = setup();
        let borrower = Address::generate(&env);
        // repay with no loan must panic with NoActiveLoan (#2)
        client.repay(&borrower);
    }

    // ── #620 tests ────────────────────────────────────────────────────────────

    #[test]
    fn test_vouch_success() {
        let (env, client) = setup();
        let borrower = Address::generate(&env);
        let voucher = Address::generate(&env);
        client.vouch(&borrower, &voucher, &100);
        let vouches = client.get_vouches(&borrower);
        assert_eq!(vouches.len(), 1);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #3)")]
    fn test_duplicate_vouch_panics() {
        let (env, client) = setup();
        let borrower = Address::generate(&env);
        let voucher = Address::generate(&env);
        client.vouch(&borrower, &voucher, &100);
        // Same voucher vouching again must panic with DuplicateVouch (#3)
        client.vouch(&borrower, &voucher, &200);
    }

    #[test]
    fn test_different_vouchers_allowed() {
        let (env, client) = setup();
        let borrower = Address::generate(&env);
        let v1 = Address::generate(&env);
        let v2 = Address::generate(&env);
        client.vouch(&borrower, &v1, &100);
        client.vouch(&borrower, &v2, &200);
        assert_eq!(client.get_vouches(&borrower).len(), 2);
    }

    // ── #621 tests ────────────────────────────────────────────────────────────

    #[test]
    #[should_panic(expected = "Error(Contract, #4)")]
    fn test_zero_stake_vouch_panics() {
        let (env, client) = setup();
        let borrower = Address::generate(&env);
        let voucher = Address::generate(&env);
        // stake = 0 must panic with ZeroStake (#4)
        client.vouch(&borrower, &voucher, &0);
    }
}
