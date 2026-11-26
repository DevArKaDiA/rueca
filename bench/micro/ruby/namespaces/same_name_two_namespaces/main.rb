module A
  class Util
    def self.run
      1
    end
  end
end

module B
  class Util
    def self.run
      2
    end
  end

  class Job
    def go
      Util.run
    end
  end
end
