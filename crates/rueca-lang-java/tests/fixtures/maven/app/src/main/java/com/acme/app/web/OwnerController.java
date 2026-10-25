package com.acme.app.web;

import com.acme.core.Inject;
import com.acme.core.Parser;
import org.springframework.beans.factory.annotation.Autowired;
import org.springframework.beans.factory.annotation.Value;
import org.springframework.core.env.Environment;
import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.RestController;

@RestController
public class OwnerController {
    @Value("${server.port:8080}")
    private int port;

    @Autowired
    private Environment env;

    @Inject("parser")
    private Parser parser;

    @GetMapping("/owners")
    public String owners(@Value("${owners.page-size}") int pageSize) {
        String home = System.getenv("OWNER_HOME");
        String url = env.getProperty("spring.datasource.url");
        parser.run();
        return home + url + pageSize;
    }
}
