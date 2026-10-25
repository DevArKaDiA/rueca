<?php
class Registry
{
    public static function boot(): void
    {
        self::load();
        static::register();
    }

    protected static function load(): void
    {
    }

    protected static function register(): void
    {
    }
}
