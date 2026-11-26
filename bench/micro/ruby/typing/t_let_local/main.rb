# typed: true
class Engine
  def start
    1
  end
end

def run(x)
  e = T.let(x, Engine)
  e.start
end
