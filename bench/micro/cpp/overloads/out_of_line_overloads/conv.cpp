#include "conv.h"

int Conv::to(int v) { return to(v, 10); }

int Conv::to(int v, int base) { return v * base; }
