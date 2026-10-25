#include "widget.h"

int render(Widget *w) {
    w->draw();
    return w->size();
}
