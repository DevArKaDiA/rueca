use crate::Account;

impl Account {
    pub fn audit(&self) {
        assert!(self.balance >= 0);
    }
}
