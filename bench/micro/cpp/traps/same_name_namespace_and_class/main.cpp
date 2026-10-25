namespace net {
int connect() { return 1; }
}

class Client {
public:
    static int connect() { return 2; }
    int open() { return net::connect() + Client::connect(); }
};

int dial() { return net::connect(); }
