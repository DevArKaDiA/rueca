struct Item {
    func open() {}
}

func use(_ item: Item?) {
    if let it = item {
        it.open()
    }
    guard let g = item else { return }
    g.open()
}

use(Item())
