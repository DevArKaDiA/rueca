package app;

abstract class Importer {
    void run() {
        read();
        store();
    }

    abstract void read();

    void store() {
    }
}

class CsvImporter extends Importer {
    @Override
    void read() {
    }
}

class Cron {
    void nightly(CsvImporter csv) {
        csv.run();
    }
}
