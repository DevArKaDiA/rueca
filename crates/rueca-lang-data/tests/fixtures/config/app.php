<?php

return [
    // The application name.
    'name' => env('APP_NAME', 'Shop'),
    'debug' => (bool) env('APP_DEBUG', false),
    'providers' => [
        'cache' => ['driver' => 'redis'],
    ],
];
