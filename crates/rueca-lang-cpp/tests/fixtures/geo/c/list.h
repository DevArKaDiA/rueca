#ifndef LIST_H
#define LIST_H

#ifdef __cplusplus
extern "C" {
#endif

typedef struct node {
    int value;
    struct node *next;
} node_t;

/* Called for every node; returns non-zero to stop. */
typedef int (*visit_fn)(node_t *n);

int list_walk(node_t *head, visit_fn fn);
int list_count(node_t *head);

#ifdef __cplusplus
}
#endif

#endif
