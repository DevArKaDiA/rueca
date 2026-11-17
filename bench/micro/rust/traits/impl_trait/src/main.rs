trait Render {
    fn render(&self) -> String;
}

struct Page;

impl Render for Page {
    fn render(&self) -> String {
        "page".to_string()
    }
}

fn show(r: impl Render) -> String {
    r.render()
}

fn make() -> impl Render {
    Page
}

fn main() {
    let s = show(Page);
    let t = make().render();
    println!("{s} {t}");
}
