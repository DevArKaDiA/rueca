package app;

class Http {
    static class Request {
        void send() {
        }
    }
}

class Ftp {
    static class Request {
        void send() {
        }
    }
}

class Client {
    void get(Http.Request r) {
        r.send();
    }
}
