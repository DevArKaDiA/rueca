<?php
class Formatter
{
    public function all(array $xs): array
    {
        return array_map(function ($x) {
            return $this->one($x);
        }, $xs);
    }

    private function one($x): string
    {
        return (string) $x;
    }
}
