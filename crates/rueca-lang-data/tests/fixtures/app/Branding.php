<?php

namespace App;

class Branding
{
    public function title(): string
    {
        $url = getenv('DATABASE_URL');
        $key = $_ENV['APP_KEY'] ?? '';
        $host = $_SERVER['HTTP_HOST'] ?? '';
        return config('app.name') . env('APP_SUFFIX', '') . config('app.providers.cache.driver');
    }
}
