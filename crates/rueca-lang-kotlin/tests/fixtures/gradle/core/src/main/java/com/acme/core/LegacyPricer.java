package com.acme.core;

public class LegacyPricer {
    public long price(Order order) {
        return order.price();
    }
}
