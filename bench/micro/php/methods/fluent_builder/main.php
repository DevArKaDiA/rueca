<?php
class QueryBuilder
{
    private array $parts = [];

    public function select(string $c): self
    {
        $this->parts[] = $c;
        return $this;
    }

    public function where(string $w): self
    {
        $this->parts[] = $w;
        return $this;
    }

    public function build(): string
    {
        return implode(' ', $this->parts);
    }
}

function query(): string
{
    return (new QueryBuilder())->select('a')->where('b')->build();
}
