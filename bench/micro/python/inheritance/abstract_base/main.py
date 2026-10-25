from abc import ABC, abstractmethod


class Job(ABC):
    @abstractmethod
    def run(self):
        raise NotImplementedError

    def safe_run(self):
        try:
            return self.run()
        except RuntimeError:
            return None


class PrintJob(Job):
    def run(self):
        print("printing")
        return True


def schedule(job: Job):
    job.safe_run()
    return job.run()


def main():
    pj = PrintJob()
    return schedule(pj)
