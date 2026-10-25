package app;

import java.util.HashMap;
import java.util.Map;

class Cache {
    private final Map<String, String> map = new HashMap<>();

    String lookup(String k) {
        String v = map.get(k);
        System.out.println(v);
        return String.valueOf(v).trim();
    }
}
