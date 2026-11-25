module App
  class Tool
    def self.use
      1
    end
  end
end

class Tool
  def self.use
    2
  end
end

class App::Runner
  def go
    Tool.use
  end
end
