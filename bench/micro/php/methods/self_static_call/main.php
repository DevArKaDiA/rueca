<?php
class Text
{
    public function clean(string $s): string
    {
        return self::squash($s);
    }

    public static function squash(string $s): string
    {
        return preg_replace('/\s+/', ' ', $s);
    }
}
