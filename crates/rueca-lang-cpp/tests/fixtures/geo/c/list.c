#include "list.h"

static int count_one(node_t *n) {
    return n->value >= 0 ? 0 : 1;
}

int list_walk(node_t *head, visit_fn fn) {
    for (node_t *cur = head; cur; cur = cur->next) {
        if (fn(cur)) return 1;
    }
    return 0;
}

int list_count(node_t *head) {
    int n = 0;
    while (head) {
        n++;
        head = head->next;
    }
    list_walk(head, count_one);
    return n;
}
