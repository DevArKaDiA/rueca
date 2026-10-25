package org.acme.tools;

import com.acme.core.Parser;

/** Command line entry point (Gradle module with a declared source directory). */
public class Cli {
    public static void main(String[] args) {
        new Parser().run();
    }
}
