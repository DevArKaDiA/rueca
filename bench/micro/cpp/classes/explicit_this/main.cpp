class Pipe {
public:
    void flush();
    void write() { this->flush(); }
};

void Pipe::flush() {}
