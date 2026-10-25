<?php
abstract class Importer
{
    public function import(): void
    {
        $this->read();
        $this->store();
    }

    abstract protected function read(): void;

    protected function store(): void
    {
    }
}

class CsvImporter extends Importer
{
    protected function read(): void
    {
    }
}
