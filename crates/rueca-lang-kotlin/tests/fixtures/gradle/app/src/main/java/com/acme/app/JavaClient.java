package com.acme.app;

import com.acme.core.Order;

public class JavaClient {
    public long run() {
        Order order = new Order(1L, 2);
        return order.price();
    }
}
