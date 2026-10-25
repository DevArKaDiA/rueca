#ifndef COUNTER_H
#define COUNTER_H
typedef struct counter counter;
counter *counter_new(void);
void counter_inc(counter *c);
int counter_get(const counter *c);
#endif
