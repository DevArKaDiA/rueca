<?php
class Service
{
    use Loggable;
}

function work(Service $s): void
{
    $s->log('x');
}
