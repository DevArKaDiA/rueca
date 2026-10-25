<?php
function outer(): void
{
    if (!function_exists('inner')) {
        function inner(): void
        {
            leaf();
        }
    }
    inner();
}

function leaf(): void
{
}
