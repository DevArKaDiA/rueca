<?php
interface Readable
{
    public function read(): string;
}

interface Stream extends Readable
{
    public function close(): void;
}

function drain(Stream $s): string
{
    $out = $s->read();
    $s->close();
    return $out;
}
