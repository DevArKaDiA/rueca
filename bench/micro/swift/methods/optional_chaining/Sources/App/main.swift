final class Node {
    var next: Node?

    func visit() {}
}

func walk(_ node: Node?) {
    node?.visit()
    node?.next?.visit()
    node!.visit()
}

walk(Node())
