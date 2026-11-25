struct Item {
    let price: Int

    func taxed() -> Int { price + 1 }
}

func total(_ items: [Item]) -> Int {
    return items.map { $0.taxed() }.reduce(0, +)
}

func cheapest(_ items: [Item]) -> Int {
    return items.map { (i: Item) -> Int in i.taxed() }.min() ?? 0
}

print(total([Item(price: 1)]) + cheapest([]))
