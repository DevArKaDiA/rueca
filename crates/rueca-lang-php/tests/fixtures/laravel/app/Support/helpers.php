<?php

namespace App\Support;

const CURRENCY = 'USD';

if (! function_exists('App\Support\format_money')) {
    /**
     * Formats cents as money.
     */
    function format_money(int $cents): string
    {
        return number_format($cents / 100, 2) . ' ' . CURRENCY;
    }
}
