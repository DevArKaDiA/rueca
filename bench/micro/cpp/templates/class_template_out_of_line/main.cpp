template <typename T>
class Stack {
public:
    void push(T v);
    bool empty() const;

private:
    int n = 0;
};

template <typename T>
void Stack<T>::push(T) { ++n; }

template <typename T>
bool Stack<T>::empty() const { return n == 0; }

bool fill(Stack<int> &s) {
    s.push(1);
    return s.empty();
}
