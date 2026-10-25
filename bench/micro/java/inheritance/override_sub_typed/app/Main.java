package app;

class Reader {
    String read() {
        return "";
    }
}

class FileReader extends Reader {
    @Override
    String read() {
        return "file";
    }
}

class Loader {
    String load(FileReader r) {
        return r.read();
    }
}
