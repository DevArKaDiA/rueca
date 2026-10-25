from typing import Dict, List


class Task:
    def run(self):
        status = "ok"
        return status


class Worker:
    def stop(self):
        status = "stopped"
        return status


def run_all(tasks: List[Task]):
    results = []
    for task in tasks:
        results.append(task.run())
    return results


def stop_all(workers: Dict[str, Worker]):
    for name, worker in workers.items():
        worker.stop()
    return len(workers)
