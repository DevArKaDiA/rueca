#pragma once
namespace store {
class Repo {
public:
    int find(int id);
    int count() const { return 3; }

private:
    int lookup(int id);
};
}
