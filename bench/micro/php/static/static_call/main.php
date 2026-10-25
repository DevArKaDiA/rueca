<?php
final class Str
{
    public static function slug(string $s): string
    {
        return strtolower(str_replace(' ', '-', $s));
    }
}

function permalink(string $title): string
{
    return '/posts/' . Str::slug($title);
}
