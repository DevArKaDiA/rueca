<?php
class Counter
{
    private int $n = 0;

    public function inc(): void
    {
        $this->n++;
    }

    public function value(): int
    {
        return $this->n;
    }
}

function tally(): int
{
    $c = new Counter();
    $c->inc();
    return $c->value();
}
