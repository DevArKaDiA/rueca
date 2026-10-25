package app;

class Tree {
    int size() {
        return 0;
    }

    class Node {
        Node() {
        }

        int depth() {
            return size();
        }
    }

    int walk() {
        Node n = new Node();
        return n.depth();
    }
}
