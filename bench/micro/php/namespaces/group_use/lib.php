<?php
namespace Lib;

class Foo
{
    public static function make(): self
    {
        return new self();
    }
}

class Bar
{
    public function __construct()
    {
    }
}
