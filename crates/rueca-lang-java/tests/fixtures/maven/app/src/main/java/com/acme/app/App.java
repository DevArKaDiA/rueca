package com.acme.app;

import static com.acme.core.Util.max;
import static com.acme.core.Util.*;

import com.acme.core.*;
import com.acme.core.Parser;
import com.acme.core.Parser.Options;
import java.util.ArrayList;
import java.util.List;

public class App {
    private final Parser parser = new Parser(new Options().withLimit(5));
    private final Registry<Circle> registry = new Registry<>();

    public static void main(String[] args) {
        App app = new App();
        app.start(args.length);
    }

    void start(int n) {
        var shapes = new ArrayList<Shape>();
        Circle c = new Circle(2.0);
        shapes.add(c);
        registry.register(c);
        Shape s = c;
        s.describe();
        parser.run();
        this.parser.parse("text");
        int m = max(n, 3);
        String joined = join(",", "a", "b");
        Color color = Color.parse("r");
        if (color == Color.RED) {
            report(color);
        }
        shapes.forEach(App::print);
        Runnable r = () -> parser.parse("lambda");
        Object o = s;
        if (o instanceof Circle circle) {
            circle.area();
        }
        List<Shape> parsed = new TextParser().parse("x");
    }

    private void report(Color color) {
        System.out.println(color.code());
    }

    private static void print(Shape shape) {
        System.out.println(shape.name());
    }
}
