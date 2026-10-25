class Animal {
public:
    virtual int speak() const { return 0; }
    virtual ~Animal() {}
};

class Cat : public Animal {
public:
    int speak() const override { return 2; }
};

int meow(const Cat &c) { return c.speak(); }
