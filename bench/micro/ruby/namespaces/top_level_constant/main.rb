class Logger
  def self.info
    2
  end
end

module App
  class Logger
    def self.info
      1
    end
  end

  class Worker
    def perform
      ::Logger.info
    end
  end
end
