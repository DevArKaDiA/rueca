package com.acme.core;

import java.lang.annotation.Retention;
import java.lang.annotation.RetentionPolicy;

/** Marks an injection point. */
@Retention(RetentionPolicy.RUNTIME)
public @interface Inject {
    String value() default "";
}
