<?php
class Animal
{
    public function breathe(): void
    {
    }
}

class Dog extends Animal
{
    public function bark(): void
    {
    }
}

function morning(Dog $d): void
{
    $d->breathe();
    $d->bark();
}
