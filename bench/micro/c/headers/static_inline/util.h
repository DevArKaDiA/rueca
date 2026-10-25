#ifndef UTIL_H
#define UTIL_H
static inline int clamp(int v, int lo, int hi)
{
    return v < lo ? lo : (v > hi ? hi : v);
}
#endif
