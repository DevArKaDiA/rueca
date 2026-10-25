package app;

class Logger {
    void log(String msg) {
        log(msg, 0);
    }

    void log(String msg, int level) {
    }
}

class Job {
    void run(Logger logger) {
        logger.log("start");
        logger.log("end", 2);
    }
}
