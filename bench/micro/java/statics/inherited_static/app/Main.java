package app;

class Base {
    static String version() {
        return "1";
    }
}

class Plugin extends Base {
    String info() {
        return Plugin.version() + version();
    }
}
