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

function persist($thing): void
{
    $thing->save();
}
