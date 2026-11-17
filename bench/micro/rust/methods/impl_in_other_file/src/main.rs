mod ext;

pub struct Account {
    pub balance: i64,
}

impl Account {
    fn deposit(&mut self, v: i64) {
        self.balance += v;
        self.audit();
    }
}

fn main() {
    let mut a = Account { balance: 0 };
    a.deposit(5);
    println!("{}", a.balance);
}
