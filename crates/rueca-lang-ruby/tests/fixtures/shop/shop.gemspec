Gem::Specification.new do |spec|
  spec.name = "shop"
  spec.version = "1.0.0"
  spec.files = Dir["lib/**/*.rb"]
  spec.add_dependency "money", "~> 6.0"
  spec.add_development_dependency "rspec"
end
