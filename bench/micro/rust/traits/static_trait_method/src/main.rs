trait Factory {
    fn create() -> Self;
}

struct Widget;

impl Factory for Widget {
    fn create() -> Self {
        Widget
    }
}

fn build<T: Factory>() -> T {
    T::create()
}

fn main() {
    let _a: Widget = build();
    let _b = Widget::create();
    let _c = <Widget as Factory>::create();
}
