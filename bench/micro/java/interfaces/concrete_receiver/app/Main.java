package app;

interface Codec {
    String encode(String s);
}

class Base64Codec implements Codec {
    @Override
    public String encode(String s) {
        return s;
    }

    String name() {
        return "b64";
    }
}

class Transport {
    String wire(Base64Codec codec) {
        return codec.name() + codec.encode("x");
    }
}
