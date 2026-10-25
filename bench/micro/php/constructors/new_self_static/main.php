<?php
class Node
{
    public function __construct(public ?Node $parent = null)
    {
    }

    public function child(): self
    {
        return new self($this);
    }

    public function sibling(): static
    {
        return new static($this->parent);
    }
}
