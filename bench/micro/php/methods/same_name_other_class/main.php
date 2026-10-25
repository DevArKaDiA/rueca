<?php
class Order
{
    public function save(): void
    {
    }
}

class Invoice
{
    public function save(): void
    {
    }
}

function persist(Order $o): void
{
    $o->save();
}
