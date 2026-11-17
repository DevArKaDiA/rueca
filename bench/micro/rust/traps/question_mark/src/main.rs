#[derive(Debug)]
struct ParseError;

#[derive(Debug)]
struct AppError;

impl From<ParseError> for AppError {
    fn from(_: ParseError) -> Self {
        AppError
    }
}

fn parse(s: &str) -> Result<u32, ParseError> {
    s.parse().map_err(|_| ParseError)
}

fn run(s: &str) -> Result<u32, AppError> {
    let v = parse(s)?;
    Ok(v + 1)
}

fn main() {
    println!("{:?}", run("4"));
}
