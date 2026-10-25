#ifndef GEO_CONFIG_H
#define GEO_CONFIG_H

// The namespace every unit header opens (like fmt's FMT_BEGIN_NAMESPACE).
#define GEO_BEGIN_NAMESPACE \
    namespace geo {         \
    inline namespace v1 {
#define GEO_END_NAMESPACE \
    }                     \
    }

#endif  // GEO_CONFIG_H
