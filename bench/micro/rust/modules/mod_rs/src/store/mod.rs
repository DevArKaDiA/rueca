mod disk;

pub fn open() -> String {
    disk::read("db")
}
