class Job
  def self.perform(args)
    new.run(args)
  end

  def run(args)
    args
  end
end
