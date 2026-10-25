<?php
trait Hello
{
    public function hello(): string
    {
        return 'hello';
    }
}

trait Hi
{
    public function hello(): string
    {
        return 'hi';
    }
}

class Greeter
{
    use Hello, Hi {
        Hello::hello insteadof Hi;
        Hi::hello as hiHello;
    }

    public function both(): string
    {
        return $this->hello() . $this->hiHello();
    }
}
