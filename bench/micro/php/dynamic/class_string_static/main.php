<?php
class Util
{
    public static function slug(string $s): string
    {
        return strtolower($s);
    }
}

function run(): string
{
    $cls = Util::class;
    return $cls::slug('A');
}
