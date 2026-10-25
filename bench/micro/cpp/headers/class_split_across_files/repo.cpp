#include "repo.h"

namespace store {
int Repo::find(int id) { return lookup(id) + count(); }
int Repo::lookup(int id) { return id; }
}
