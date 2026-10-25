class Animal {
public:
    virtual int speak() const { return 0; }
    virtual ~Animal() {}
};

class Dog : public Animal {
public:
    int speak() const override { return 1; }
};

int chorus(const Animal &a) { return a.speak(); }
